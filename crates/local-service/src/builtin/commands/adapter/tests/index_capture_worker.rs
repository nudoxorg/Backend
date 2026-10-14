//! The source capture of a finished scan runs on its own worker. The owner
//! hands over the scan and keeps answering requests; it never prepares, stages
//! or commits the scan's source rows itself.
use super::index_request_admission_tests::{dispatch, prepare_project, status, surface};
use super::*;
use crate::builtin::commands::adapter::index_publication_worker::{TestHook, TestStage};
use backend_library::{
    IndexOperationKey, IndexOperationObservation, IndexOperationState, SurfaceCommand, SurfaceReply,
};
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, Instant};

/// Parks the capture worker at `stage` until the returned sender is used.
fn park_worker_at(
    fixture: &mut AdapterFixture,
    stage: TestStage,
) -> (Receiver<()>, std::sync::mpsc::SyncSender<()>) {
    let (entered, observed) = sync_channel(1);
    let (release, released) = sync_channel(1);
    fixture
        .adapter
        .as_mut()
        .expect("adapter")
        .publication_test_hook = Some(TestHook {
        stage,
        run: Box::new(move || {
            entered.send(()).expect("test observes the parked worker");
            released
                .recv_timeout(Duration::from_secs(60))
                .expect("bounded worker release");
        }),
    });
    (observed, release)
}

fn start_keyed(fixture: &mut AdapterFixture, key: IndexOperationKey, id: u64) {
    let package = backend_library::PackageReference::parse(fixture.label.clone())
        .expect("actual project address");
    let reply = surface(
        dispatch(
            fixture,
            id,
            SurfaceCommand::IndexOperationStart {
                operation_key: key,
                package,
                execution_intent: CompileExecutionIntent::Interactive,
            },
        )
        .expect("keyed admission"),
    );
    assert!(matches!(
        reply,
        SurfaceReply::IndexOperationStarted(IndexOperationObservation::Known(_))
    ));
}

/// Runs the owner until its index work settles. The deadline is generous: every
/// step is a real durable write, and these controls also run on loaded disks.
fn drain(fixture: &mut AdapterFixture) {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let (adapter, daemon) = fixture.parts();
        let _ = adapter.poll_deferred(daemon);
        if adapter.indexing.is_none() && adapter.waiting.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "original owner work did not settle"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn tick(fixture: &mut AdapterFixture) {
    let (adapter, daemon) = fixture.parts();
    let _ = adapter.poll_deferred(daemon);
}

#[test]
fn owner_keeps_serving_while_the_capture_prepares_on_its_worker() {
    let mut fixture = AdapterFixture::new();
    prepare_project(&fixture);
    let (entered, release) = park_worker_at(&mut fixture, TestStage::BeforeScan);
    let operation = IndexOperationKey::from_bytes([0xc1; 32]).expect("durable operation");
    start_keyed(&mut fixture, operation, 701);
    let base_sequence = fixture
        .parts()
        .1
        .engine()
        .daemon()
        .owner()
        .head()
        .sequence();

    // The scan finishes off the owner, which then hands it to the capture
    // worker; the worker parks before it has touched anything durable.
    let deadline = Instant::now() + Duration::from_secs(60);
    while entered.try_recv().is_err() {
        tick(&mut fixture);
        assert!(
            Instant::now() < deadline,
            "the scan never reached its capture worker"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    {
        let (adapter, _) = fixture.parts();
        let job = adapter.indexing.as_ref().expect("active index job");
        assert!(
            matches!(job.work, IndexJobWork::PublicationWorker { .. }),
            "the capture must be in its worker, not on the owner"
        );
    }

    // With the worker parked mid-preparation the owner keeps polling and
    // answering reads promptly, the job stays active at its scan stage, and
    // nothing durable has changed.
    for _ in 0..25 {
        let started = Instant::now();
        tick(&mut fixture);
        let observed = status(&mut fixture, operation);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "owner turn took {:?} while the capture worker was parked",
            started.elapsed()
        );
        assert!(
            matches!(observed.state, IndexOperationState::Active { .. }),
            "{observed:?}"
        );
        assert_eq!(
            fixture
                .parts()
                .1
                .engine()
                .daemon()
                .owner()
                .head()
                .sequence(),
            base_sequence,
            "the owner must not have committed the capture"
        );
    }

    release.send(()).expect("release the capture worker");
    drain(&mut fixture);
    let terminal = status(&mut fixture, operation);
    // The closed fixture has no compiler: the source and Pending capture still
    // committed, so the receipt retains them and the job ends as a refusal.
    assert!(
        matches!(terminal.state, IndexOperationState::Failed { .. }),
        "{terminal:?}"
    );
    assert!(
        terminal.source_capture.is_some(),
        "the capture worker must have recorded the structural source receipt"
    );
    assert!(
        fixture
            .parts()
            .1
            .engine()
            .daemon()
            .owner()
            .head()
            .sequence()
            > base_sequence,
        "the capture transition must have been selected"
    );
    // The worker that published the last root counted its files, so a status
    // request is served from that count rather than decoding every row.
    let (adapter, daemon) = fixture.parts();
    let head = *daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .root()
        .as_bytes();
    let counted = adapter
        .ingest_progress
        .borrow()
        .as_ref()
        .map(|memo| memo.root);
    assert_eq!(
        counted,
        Some(head),
        "the publication worker must hand over the ingest counts of its root"
    );
}

#[test]
fn project_without_compiler_sources_publishes_through_the_capture_worker() {
    let mut fixture = AdapterFixture::new();
    fs::write(
        Path::new(&fixture.label).join("README.md"),
        "# Notes\nNo compilable source here.\n",
    )
    .expect("real text file");
    let operation = IndexOperationKey::from_bytes([0xc2; 32]).expect("durable operation");
    start_keyed(&mut fixture, operation, 702);
    drain(&mut fixture);
    let terminal = status(&mut fixture, operation);
    assert!(
        matches!(terminal.state, IndexOperationState::Published(_)),
        "{terminal:?}"
    );
    assert!(
        terminal.source_capture.is_none(),
        "nothing needed a Pending capture"
    );
}

#[test]
fn preparing_a_scan_against_a_snapshot_commits_nothing() {
    use crate::builtin::commands::index::{self, ScanPlan};
    let mut fixture = AdapterFixture::new();
    let (package, label) = fixture.add_target();
    fs::write(
        Path::new(&label).join("pyproject.toml"),
        "[project]\nname=\"prepared_only\"\nversion=\"1.0.0\"\n",
    )
    .expect("real manifest");
    fs::write(
        Path::new(&label).join("source.py"),
        "def prepared_only(value: int) -> int:\n    return value + 1\n",
    )
    .expect("real source");
    let (adapter, daemon) = fixture.parts();
    let work = index::capture_index_scan(
        daemon,
        package,
        &label,
        Path::new(&label),
        None,
        0x7a1,
        CompileExecutionIntent::Interactive,
        false,
        Arc::new(AtomicBool::new(false)),
    )
    .expect("pinned scan input");
    // Capturing the scan input reads nothing from the store: the prior
    // frontier is loaded by the scan worker.
    let scan = index::run_index_scan(work).unwrap_or_else(|_| panic!("actual source scan"));
    let before = daemon.engine().daemon().owner().head().sequence();
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let plan = index::prepare_scan_plan(
        &snapshot,
        scan,
        &adapter.compiler,
        &mut adapter.semantic_authority,
        None,
    )
    .unwrap_or_else(|error| panic!("prepare scan: {error:?}"));
    assert!(
        matches!(plan, ScanPlan::Capture(_)),
        "real Python source needs a Pending capture"
    );
    assert_eq!(
        daemon.engine().daemon().owner().head().sequence(),
        before,
        "preparing a scan must not select a workspace transition"
    );
}

#[test]
fn refusal_commit_and_view_publish_on_a_worker_not_the_owner() {
    // The closed fixture has no Python compiler, so every profile is refused
    // after the capture committed: the refusal's own transition and the view
    // it installs are the largest durable steps left at the end of the job.
    let mut fixture = AdapterFixture::new();
    prepare_project(&fixture);
    let operation = IndexOperationKey::from_bytes([0xc3; 32]).expect("durable operation");
    start_keyed(&mut fixture, operation, 703);
    // Let the capture run normally, then park the next publication worker
    // (the refusal) before it stages anything.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        tick(&mut fixture);
        let compiling = fixture
            .parts()
            .0
            .indexing
            .as_ref()
            .is_some_and(|job| matches!(job.work, IndexJobWork::Compiling { .. }));
        if compiling {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the capture never reached compilation"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let (entered, release) = park_worker_at(&mut fixture, TestStage::BeforeStage);
    while entered.try_recv().is_err() {
        tick(&mut fixture);
        assert!(
            Instant::now() < deadline,
            "the refusal never reached its publication worker"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let base_sequence = fixture
        .parts()
        .1
        .engine()
        .daemon()
        .owner()
        .head()
        .sequence();
    {
        let job = fixture.parts().0.indexing.as_ref().expect("active job");
        assert!(
            matches!(job.work, IndexJobWork::PublicationWorker { .. }),
            "the refusal must be committed by its worker"
        );
    }
    for _ in 0..25 {
        let started = Instant::now();
        tick(&mut fixture);
        let observed = status(&mut fixture, operation);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "owner turn took {:?} while the refusal worker was parked",
            started.elapsed()
        );
        assert!(
            matches!(observed.state, IndexOperationState::Active { .. }),
            "{observed:?}"
        );
        assert_eq!(
            fixture
                .parts()
                .1
                .engine()
                .daemon()
                .owner()
                .head()
                .sequence(),
            base_sequence,
            "the owner must not have committed the refusal"
        );
    }
    release.send(()).expect("release the refusal worker");
    drain(&mut fixture);
    let terminal = status(&mut fixture, operation);
    assert!(
        matches!(terminal.state, IndexOperationState::Failed { .. }),
        "{terminal:?}"
    );
    assert!(
        fixture
            .parts()
            .1
            .engine()
            .daemon()
            .owner()
            .head()
            .sequence()
            > base_sequence,
        "the refusal transition must have been selected"
    );
}
