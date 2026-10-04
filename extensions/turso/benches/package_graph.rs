//! Checked Turso package-graph publication, root-only moves, and one-edge edits.
//!
//! This is a deterministic synthetic projection fixture, not an upstream
//! registry benchmark. Complete admitted view roots and checked graph facts
//! enter through the same selected-generation admission and revision-fenced
//! APIs used by production callers. Fixture decoding and view construction are
//! outside the timed database operations. Local revisions fence only cached
//! state and do not establish upstream source freshness.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "the fixed benchmark fixture is intentionally fail-fast and bounded"
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
    AuthorityScopeClaim, Basis, CheckedPackageGraphFacts, Coverage, CoverageCapability,
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope, Frontier,
    PackageDependencyRecord, PackageDependencyTarget, PackageGraphSourceKey, PackageReference,
    ProducerObservationClaims, ProducerObservationVerifier, RegistryEcosystem, Row, RowId,
    ViewRoot, admit_complete_scope, admit_producer_observation, branch_key, log_key,
    object_version, package_key, view_key, view_state_root,
};

const SOURCE_COUNT: usize = 4_096;
const EDGES_PER_SOURCE: usize = 8;
const EDGES: usize = SOURCE_COUNT * EDGES_PER_SOURCE;
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
        b"package-graph-benchmark-view".to_vec(),
    );
    let observation =
        admit_producer_observation(raw.clone(), &ExactObservation(raw)).expect("observation");
    let coverage = admit_complete_scope(declaration, observation).expect("coverage");
    CoverageCapability::from_authorized_with_evidence(
        coverage,
        b"package-graph-benchmark-view".to_vec(),
    )
    .expect("capability")
}

fn admitted_view(sequence: u64) -> ViewRoot {
    let source = view_state_root(&[]);
    let object = object_version(b"package-graph-benchmark-view");
    let basis = Basis::with_context(source, object, branch_key("main"), log_key("graph"), 1);
    let package = package_key("graph-fixture");
    ViewRoot::new_checked(
        view_key(b"package-graph-benchmark-view"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, sequence),
        vec![Row::new(
            RowId::Package(package),
            basis,
            "graph benchmark package",
        )],
        vec![Coverage::Complete],
        capability(object),
    )
    .expect("admitted benchmark view")
}

fn graph_observation(
    sequence: u64,
    first_requirement: &str,
) -> (ViewRoot, CheckedPackageGraphFacts) {
    // Construct one coherent benchmark-only root/facts observation after the
    // caller captures its projection revision; this is not upstream evidence.
    (
        admitted_view(sequence),
        CheckedPackageGraphFacts::new(facts(first_requirement))
            .expect("checked benchmark graph observation"),
    )
}

fn edge(source: &PackageReference, index: usize, requirement: &str) -> PackageDependencyRecord {
    PackageDependencyRecord::new(
        source.clone(),
        PackageDependencyTarget::new(
            RegistryEcosystem::Cargo,
            format!("dep{index}"),
            requirement,
            None,
        )
        .expect("dependency target"),
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
                .expect("source package reference");
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
    panic!("could not allocate a unique private package-graph benchmark root");
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

fn count_generation_tree(path: &Path, footprint: &mut PhysicalFootprint) {
    let metadata = fs::symlink_metadata(path).expect("Turso generation entry metadata");
    assert!(
        !metadata.file_type().is_symlink(),
        "Turso generation symlink"
    );
    if metadata.file_type().is_file() {
        let bytes = metadata.len();
        footprint.namespace_file_bytes = footprint.namespace_file_bytes.saturating_add(bytes);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if name.ends_with("-wal") || name.ends_with(".wal") {
            footprint.wal_file_bytes = footprint.wal_file_bytes.saturating_add(bytes);
        } else if name.ends_with("-shm") || name.ends_with(".shm") {
            footprint.shm_file_bytes = footprint.shm_file_bytes.saturating_add(bytes);
        }
        footprint.allocated_namespace_bytes = footprint
            .allocated_namespace_bytes
            .saturating_add(allocated_bytes(&metadata));
        return;
    }
    assert!(
        metadata.file_type().is_dir(),
        "unexpected Turso generation object"
    );
    footprint.allocated_namespace_bytes = footprint
        .allocated_namespace_bytes
        .saturating_add(allocated_bytes(&metadata));
    for entry in fs::read_dir(path).expect("read Turso namespace") {
        count_generation_tree(&entry.expect("Turso generation entry").path(), footprint);
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
    assert_eq!(selector.len(), 155, "selector record length");
    assert_eq!(&selector[..8], b"BPTSEL01", "selector magic");
    assert_eq!(selector[8..10], [0, 1], "selector format version");
    assert_eq!(
        u64::from_be_bytes(selector[10..18].try_into().expect("selector generation")),
        generation,
        "revision and selector generation"
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
    assert_eq!(marker.len(), 155, "generation marker length");
    assert_eq!(&marker[..8], b"BPTGEN01", "generation marker magic");
    assert_eq!(marker[8..10], [0, 1], "generation marker format version");
    assert_eq!(
        u64::from_be_bytes(marker[10..18].try_into().expect("marker generation")),
        generation,
        "selected generation marker"
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
        let generation_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let digits = generation_name
            .strip_prefix('g')
            .expect("generation prefix");
        assert!(
            digits.len() == 16
                && digits
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "canonical generation directory"
        );
        footprint.generation_count = footprint.generation_count.saturating_add(1);
        let before = footprint.namespace_file_bytes;
        count_generation_tree(&path, &mut footprint);
        let generation_bytes = footprint.namespace_file_bytes.saturating_sub(before);
        if generation_name == selected_name {
            footprint.selected_generation_bytes = generation_bytes;
        } else {
            footprint.retained_generation_bytes = footprint
                .retained_generation_bytes
                .saturating_add(generation_bytes);
        }
    }
    for entry in fs::read_dir(&namespace).expect("read namespace controls") {
        let path = entry.expect("namespace control").path();
        match path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
        {
            "generations" => continue,
            "selected" => {}
            "last-generation" => {
                footprint.counter_bytes = fs::metadata(&path).expect("counter metadata").len();
            }
            "selection.lock" => {
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

async fn verify_selected(
    projection: &TursoProjection,
    view: &ViewRoot,
    checked: &CheckedPackageGraphFacts,
    first_requirement: &str,
) {
    let revision = projection
        .package_graph_revision()
        .await
        .expect("selected graph revision");
    assert_eq!(
        revision.view_root(),
        *view.root().as_bytes(),
        "selected view root"
    );
    assert_eq!(
        revision.facts_witness(),
        Some(checked.witness()),
        "selected graph facts witness"
    );

    let package_id = RowId::Package(package_key("graph-fixture")).stable_key();
    let projected = projection
        .lookup_label("graph benchmark package", 1)
        .await
        .expect("selected package label query");
    assert_eq!(
        projected.root,
        *view.root().as_bytes(),
        "row query root fence"
    );
    assert_eq!(projected.ids.as_ref(), [package_id], "selected package row");

    let source = PackageReference::parse("pkg:cargo/app0@1.0.0").expect("source reference");
    let forward = projection
        .package_dependencies(&source)
        .await
        .expect("selected forward dependency query");
    assert_eq!(
        forward.root.as_ref(),
        view.root().as_bytes(),
        "forward root fence"
    );
    assert_eq!(
        forward.facts_witness,
        checked.witness(),
        "forward facts witness"
    );
    assert_eq!(forward.edges.len(), EDGES_PER_SOURCE, "forward edge count");
    assert!(forward.edges.iter().any(|edge| {
        edge.target.name.as_str() == "dep0" && edge.target.requirement.as_str() == first_requirement
    }));

    let target =
        PackageDependencyTarget::new(RegistryEcosystem::Cargo, "dep0", first_requirement, None)
            .expect("reverse query target");
    let reverse = projection
        .package_dependents(&target)
        .await
        .expect("selected reverse dependency query");
    assert_eq!(
        reverse.root.as_ref(),
        view.root().as_bytes(),
        "reverse root fence"
    );
    assert_eq!(
        reverse.facts_witness,
        checked.witness(),
        "reverse facts witness"
    );
    assert_eq!(reverse.edges.len(), 1, "reverse edge count");
    assert_eq!(reverse.edges.first().expect("reverse edge").source, source);
}

fn percentile(samples: &mut [u128], rank: usize) -> u128 {
    samples.sort_unstable();
    samples[rank.min(samples.len().saturating_sub(1))]
}

fn format_root(root: &[u8; 32]) -> String {
    root.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn main() {
    futures_executor::block_on(async {
        let path = private_temp_root("package-graph-root-move");
        let database = path.join("projection.turso");
        let stable = CheckedPackageGraphFacts::new(facts("^1")).expect("checked stable facts");
        let edited = CheckedPackageGraphFacts::new(facts("^2")).expect("checked one-edge edit");
        assert_ne!(stable.witness(), edited.witness());
        assert_eq!(
            stable
                .source_witnesses()
                .iter()
                .zip(edited.source_witnesses())
                .filter(|(left, right)| left != right)
                .count(),
            1,
            "the edited snapshot must change exactly one source witness"
        );

        let base_view = admitted_view(0);
        let cold_started = Instant::now();
        let mut projection = TursoProjection::seed_if_empty(
            &database,
            ProjectionSeed::new(&base_view, ProjectionGraphSeed::Checked(&stable)),
        )
        .await
        .expect("complete view and checked graph seed");
        let cold_seed_ns = cold_started.elapsed().as_nanos();
        let seed_revision = projection.revision().await.expect("cold selected revision");
        let selected_generation = seed_revision.generation().get();
        assert_eq!(seed_revision.root(), *base_view.root().as_bytes());
        verify_selected(&projection, &base_view, &stable, "^1").await;

        drop(projection);
        let reopen_started = Instant::now();
        let mut projection = TursoProjection::open(&database)
            .await
            .expect("selected-only cold reopen");
        let graph_revision = projection
            .package_graph_revision()
            .await
            .expect("graph revision before source selection");
        assert_eq!(graph_revision.generation().get(), selected_generation);
        let (reopened_view, reopened_facts) = graph_observation(0, "^1");
        assert_eq!(graph_revision.view_root(), *reopened_view.root().as_bytes());
        assert_eq!(reopened_facts.witness(), stable.witness());
        let reopen_update = projection
            .synchronize_checked_package_graph_from(
                graph_revision,
                reopened_view.root(),
                &reopened_facts,
            )
            .await
            .expect("fenced cold-restart graph reuse");
        let cold_reopen_ns = reopen_started.elapsed().as_nanos();
        assert_eq!(
            reopen_update,
            ProjectionUpdate::Reused { rows: EDGES as u64 }
        );
        verify_selected(&projection, &reopened_view, &reopened_facts, "^1").await;

        let mut reuse_samples = [0_u128; SAMPLES];
        for sample in 0..(WARMUPS + SAMPLES) {
            let expected = projection
                .package_graph_revision()
                .await
                .expect("reuse graph revision");
            let (observed_view, observed_facts) = graph_observation(0, "^1");
            assert_eq!(observed_view.root(), base_view.root());
            assert_eq!(observed_facts.witness(), stable.witness());
            let started = Instant::now();
            let update = projection
                .synchronize_checked_package_graph_from(
                    expected,
                    observed_view.root(),
                    &observed_facts,
                )
                .await
                .expect("fenced exact graph reuse");
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(
                black_box(update),
                ProjectionUpdate::Reused { rows: EDGES as u64 }
            );
            if sample >= WARMUPS {
                reuse_samples[sample - WARMUPS] = elapsed;
            }
        }
        verify_selected(&projection, &base_view, &stable, "^1").await;

        let mut view_root_move_samples = [0_u128; SAMPLES];
        let mut root_move_samples = [0_u128; SAMPLES];
        let mut final_moved_view = None;
        for sample in 0..(WARMUPS + SAMPLES) {
            let expected_view = projection.revision().await.expect("view root-move fence");
            let target_view = admitted_view(
                u64::try_from(sample + 1).expect("move view sequence"),
            );
            assert_ne!(
                expected_view.root(),
                *target_view.root().as_bytes(),
                "root-move fixture must change the admitted view root"
            );
            let view_started = Instant::now();
            let view_update = projection
                .synchronize_from(expected_view, &target_view)
                .await
                .expect("fenced complete view-root move");
            let view_elapsed = view_started.elapsed().as_nanos();
            assert_eq!(view_update, ProjectionUpdate::Rebuilt { rows: 1 });

            let graph_revision = projection
                .package_graph_revision()
                .await
                .expect("graph fence before source fact selection");
            assert_eq!(graph_revision.view_root(), *target_view.root().as_bytes());
            let (observed_view, observed_facts) = graph_observation(
                u64::try_from(sample + 1).expect("graph observation sequence"),
                "^1",
            );
            assert_eq!(observed_view.root(), target_view.root());
            assert_eq!(observed_facts.witness(), stable.witness());
            let started = Instant::now();
            let update = projection
                .synchronize_checked_package_graph_from(
                    graph_revision,
                    observed_view.root(),
                    &observed_facts,
                )
                .await
                .expect("fenced root-only graph move");
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(
                black_box(update),
                ProjectionUpdate::Rebuilt { rows: EDGES as u64 }
            );
            let after = projection
                .package_graph_revision()
                .await
                .expect("selected root-move graph revision");
            assert_eq!(after.view_root(), *target_view.root().as_bytes());
            assert_eq!(after.facts_witness(), Some(observed_facts.witness()));
            assert_eq!(after.generation().get(), selected_generation);
            verify_selected(&projection, &observed_view, &observed_facts, "^1").await;
            final_moved_view = Some(target_view);
            if sample >= WARMUPS {
                view_root_move_samples[sample - WARMUPS] = view_elapsed;
                root_move_samples[sample - WARMUPS] = elapsed;
            }
        }
        let final_moved_view = final_moved_view.expect("final root-move view");
        verify_selected(&projection, &final_moved_view, &stable, "^1").await;

        let edit_path = private_temp_root("package-graph-one-edge");
        let edit_database = edit_path.join("projection.turso");
        let edit_view = admitted_view(0);
        let mut edit_projection = TursoProjection::seed_if_empty(
            &edit_database,
            ProjectionSeed::new(&edit_view, ProjectionGraphSeed::Checked(&stable)),
        )
        .await
        .expect("complete graph edit baseline seed");
        let edit_generation = edit_projection
            .revision()
            .await
            .expect("edit baseline selected revision")
            .generation()
            .get();
        let mut one_edge_samples = [0_u128; SAMPLES];
        for sample in 0..(WARMUPS + SAMPLES) {
            let expected = edit_projection
                .package_graph_revision()
                .await
                .expect("one-edge graph revision");
            let requirement = if sample % 2 == 0 { "^2" } else { "^1" };
            let (observed_view, observed_facts) = graph_observation(0, requirement);
            assert_eq!(observed_view.root(), edit_view.root());
            assert_eq!(
                observed_facts.witness(),
                if sample % 2 == 0 {
                    edited.witness()
                } else {
                    stable.witness()
                }
            );
            let started = Instant::now();
            let update = edit_projection
                .synchronize_checked_package_graph_from(
                    expected,
                    observed_view.root(),
                    &observed_facts,
                )
                .await
                .expect("fenced one-edge graph edit");
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(
                black_box(update),
                ProjectionUpdate::Rebuilt { rows: EDGES as u64 }
            );
            let after = edit_projection
                .package_graph_revision()
                .await
                .expect("selected one-edge graph revision");
            assert_eq!(after.generation().get(), edit_generation);
            assert_eq!(after.view_root(), *observed_view.root().as_bytes());
            assert_eq!(after.facts_witness(), Some(observed_facts.witness()));
            verify_selected(
                &edit_projection,
                &observed_view,
                &observed_facts,
                requirement,
            )
            .await;
            if sample >= WARMUPS {
                one_edge_samples[sample - WARMUPS] = elapsed;
            }
        }
        verify_selected(&edit_projection, &edit_view, &stable, "^1").await;

        let root_footprint = selected_footprint(&database, selected_generation);
        let edit_footprint = selected_footprint(&edit_database, edit_generation);
        println!(
            "package_graph fixture=deterministic_synthetic sources={SOURCE_COUNT} edges={EDGES} warmups={WARMUPS} samples={SAMPLES} cold_seed_ns={cold_seed_ns} cold_selected_reopen_reuse_ns={cold_reopen_ns} exact_reuse_median_ns={} exact_reuse_p95_ns={} view_root_move_median_ns={} view_root_move_p95_ns={} graph_root_only_move_median_ns={} graph_root_only_move_p95_ns={} one_edge_median_ns={} one_edge_p95_ns={} final_view_root={} facts_witness={} generation={} generation_count={} namespace_file_bytes={} allocated_namespace_bytes={} selected_generation_bytes={} retained_generation_bytes={} selector_bytes={} counter_bytes={} gate_bytes={} wal_bytes={} shm_bytes={} edit_namespace_file_bytes={} edit_allocated_namespace_bytes={} edit_selected_generation_bytes={} edit_wal_bytes={} edit_shm_bytes={}",
            percentile(&mut reuse_samples, SAMPLES / 2),
            percentile(&mut reuse_samples, SAMPLES * 95 / 100),
            percentile(&mut view_root_move_samples, SAMPLES / 2),
            percentile(&mut view_root_move_samples, SAMPLES * 95 / 100),
            percentile(&mut root_move_samples, SAMPLES / 2),
            percentile(&mut root_move_samples, SAMPLES * 95 / 100),
            percentile(&mut one_edge_samples, SAMPLES / 2),
            percentile(&mut one_edge_samples, SAMPLES * 95 / 100),
            format_root(final_moved_view.root().as_bytes()),
            format_root(&stable.witness()),
            selected_generation,
            root_footprint.generation_count,
            root_footprint.namespace_file_bytes,
            root_footprint.allocated_namespace_bytes,
            root_footprint.selected_generation_bytes,
            root_footprint.retained_generation_bytes,
            root_footprint.selector_bytes,
            root_footprint.counter_bytes,
            root_footprint.gate_bytes,
            root_footprint.wal_file_bytes,
            root_footprint.shm_file_bytes,
            edit_footprint.namespace_file_bytes,
            edit_footprint.allocated_namespace_bytes,
            edit_footprint.selected_generation_bytes,
            edit_footprint.wal_file_bytes,
            edit_footprint.shm_file_bytes,
        );

        drop(edit_projection);
        drop(projection);
        fs::remove_dir_all(edit_path).expect("remove owned edit benchmark root");
        fs::remove_dir_all(path).expect("remove owned root-move benchmark root");
    });
}
