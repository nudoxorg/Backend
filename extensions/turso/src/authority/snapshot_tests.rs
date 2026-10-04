//! Same-engine snapshot contracts against real multiprocess Turso handles.

#![allow(clippy::expect_used, clippy::panic)]

use super::{
    AuthorityNamespace, SourceObservation, SourceObservationValue, TursoAuthority,
    snapshot::{AuthoritySnapshotBudget, AuthoritySnapshotError},
};
use crate::{
    connection::BUSY_TIMEOUT,
    process_harness::{self, Database, Peer},
    sharing::SharedWalBackend,
};
use backend_platform::{CreatedDirectory, DirectoryCapability};
use std::{
    fs,
    future::Future,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    task::{Context, Poll, Wake, Waker},
    thread,
    time::Duration,
};

const WRITER_PEER: &str = "authority::snapshot_tests::snapshot_writer_peer";
const COLD_REOPEN_PEER: &str = "authority::snapshot_tests::snapshot_cold_reopen_peer";
const SNAPSHOT_BASELINE: u64 = 2_500;
const MAX_TEST_DIRECTORY_ENTRIES: usize = 32;

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

struct PrivateTestRoot {
    path: PathBuf,
    receipt: CreatedDirectory,
}

impl PrivateTestRoot {
    fn new() -> Self {
        Self::with_suffix("")
    }

    fn with_suffix(suffix: &str) -> Self {
        let parent_path = std::env::temp_dir();
        let parent = DirectoryCapability::open(&parent_path).expect("temporary directory handle");
        let name = format!(
            ".backend-turso-snapshot-{}-{}{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed),
            suffix,
        );
        let receipt = parent
            .create_private_dir_tracked(&name)
            .expect("create private snapshot test root");
        Self {
            path: parent_path.join(name),
            receipt,
        }
    }

    fn capability(&self) -> &DirectoryCapability {
        self.receipt.capability()
    }

    fn remove_all(&self) {
        assert!(
            self.receipt
                .capability()
                .entries(1)
                .expect("inspect private test root")
                .is_empty(),
            "only an empty test root may be removed"
        );
        self.receipt
            .remove_all(0)
            .expect("remove private test root through its retained receipt");
    }
}

impl Drop for PrivateTestRoot {
    fn drop(&mut self) {
        if self
            .receipt
            .capability()
            .entries(1)
            .is_ok_and(|entries| entries.is_empty())
        {
            let _ = self.receipt.remove_all(0);
        }
    }
}

fn snapshot_namespace() -> AuthorityNamespace {
    AuthorityNamespace::package_metadata(
        "pkg:turso-owner-snapshot-test",
        "registry:test-snapshot",
        "main",
        "stable",
    )
    .expect("snapshot namespace")
}

fn observation(count: u64) -> SourceObservation {
    SourceObservation::new(
        snapshot_namespace(),
        Some([0x5a; 32]),
        count,
        SourceObservationValue::KnownCount(count),
    )
    .expect("snapshot source observation")
}

async fn seed_large_history(authority: &mut TursoAuthority) -> u64 {
    let initial = authority
        .record_source_observation(observation(1))
        .await
        .expect("initialize snapshot test scope");
    assert_eq!(initial.sequence(), 1);

    let namespace = snapshot_namespace();
    let connection = authority.raw_connection();
    connection
        .execute("BEGIN IMMEDIATE", ())
        .await
        .expect("begin one large valid fixture transaction");
    let reason = "snapshot-history-".to_owned() + &"x".repeat(4_000);
    for sequence in 2..SNAPSHOT_BASELINE {
        connection
            .execute(
                "INSERT INTO backend_index_authority_observations(\
                    package, source, branch, environment, plane_kind, profile, sequence, revision, \
                    observed_at_ms, value_kind, known_count, unavailable_reason\
                 ) VALUES (?1, ?2, ?3, ?4, 0, '', ?5, ?6, ?7, 3, NULL, ?8)",
                turso::params![
                    namespace.package(),
                    namespace.source(),
                    namespace.branch(),
                    namespace.environment(),
                    i64::try_from(sequence).expect("sequence fits SQLite integer"),
                    Some(&[0x5a_u8; 32][..]),
                    i64::try_from(sequence).expect("timestamp fits SQLite integer"),
                    reason.as_str(),
                ],
            )
            .await
            .expect("insert bounded valid historical source observation");
    }
    connection
        .execute(
            "INSERT INTO backend_index_authority_observations(\
                package, source, branch, environment, plane_kind, profile, sequence, revision, \
                observed_at_ms, value_kind, known_count, unavailable_reason\
             ) VALUES (?1, ?2, ?3, ?4, 0, '', ?5, ?6, ?7, 1, ?5, NULL)",
            turso::params![
                namespace.package(),
                namespace.source(),
                namespace.branch(),
                namespace.environment(),
                i64::try_from(SNAPSHOT_BASELINE).expect("baseline fits SQLite integer"),
                Some(&[0x5a_u8; 32][..]),
                i64::try_from(SNAPSHOT_BASELINE).expect("timestamp fits SQLite integer"),
            ],
        )
        .await
        .expect("insert baseline head observation");
    connection
        .execute(
            "UPDATE backend_index_authority_scopes SET latest_observation=?1 \
             WHERE package=?2 AND source=?3 AND branch=?4 AND environment=?5 \
               AND plane_kind=0 AND profile=''",
            turso::params![
                i64::try_from(SNAPSHOT_BASELINE).expect("baseline fits SQLite integer"),
                namespace.package(),
                namespace.source(),
                namespace.branch(),
                namespace.environment(),
            ],
        )
        .await
        .expect("advance fixture scope head with its complete history");
    connection
        .execute("COMMIT", ())
        .await
        .expect("commit large valid fixture history");
    SNAPSHOT_BASELINE
}

async fn open_read_only(path: &Path) -> (turso::Database, turso::Connection) {
    let text = path.to_str().expect("snapshot path is UTF-8");
    let backend = SharedWalBackend::detect().expect("shared Turso backend");
    let database = backend
        .open_database(
            backend
                .builder(text)
                .read_only(true)
                .experimental_index_method(true),
        )
        .await
        .expect("same-engine read-only snapshot open");
    let connection = database.connect().expect("snapshot connection");
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .expect("snapshot busy timeout");
    (database, connection)
}

async fn read_latest_value(connection: &turso::Connection) -> (u64, u64, u64) {
    let namespace = snapshot_namespace();
    let mut rows = connection
        .query(
            "SELECT s.latest_observation, o.value_kind, o.known_count, o.observed_at_ms \
             FROM backend_index_authority_scopes AS s \
             JOIN backend_index_authority_observations AS o \
               ON o.package=s.package AND o.source=s.source AND o.branch=s.branch \
              AND o.environment=s.environment AND o.plane_kind=s.plane_kind \
              AND o.profile=s.profile AND o.sequence=s.latest_observation \
             WHERE s.package=?1 AND s.source=?2 AND s.branch=?3 AND s.environment=?4 \
               AND s.plane_kind=0 AND s.profile=''",
            turso::params![
                namespace.package(),
                namespace.source(),
                namespace.branch(),
                namespace.environment(),
            ],
        )
        .await
        .expect("read exact selected source head");
    let row = rows
        .next()
        .await
        .expect("read source head row")
        .expect("source head exists");
    let sequence =
        u64::try_from(row.get::<i64>(0).expect("head sequence")).expect("positive sequence");
    let value_kind: i64 = row.get(1).expect("value kind");
    let count = u64::try_from(row.get::<i64>(2).expect("known count")).expect("nonnegative count");
    let observed_at = u64::try_from(row.get::<i64>(3).expect("observation time"))
        .expect("nonnegative observation time");
    assert_eq!(value_kind, 1, "snapshot head retains its known-count fact");
    assert!(rows.next().await.expect("check exact row count").is_none());
    (sequence, count, observed_at)
}

fn cold_file_digest(path: &Path) -> (u64, [u8; 32]) {
    let parent_path = path.parent().expect("snapshot has a parent directory");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("snapshot filename is UTF-8");
    let directory = DirectoryCapability::open(parent_path).expect("open snapshot directory");
    let mut file = directory
        .open_private_file(name)
        .expect("open private snapshot file in cold reader");
    let bytes = file.metadata().expect("snapshot metadata").len();
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read_len = file.read(&mut buffer).expect("read snapshot bytes");
        if read_len == 0 {
            break;
        }
        hasher.update(&buffer[..read_len]);
    }
    (bytes, *hasher.finalize().as_bytes())
}

fn digest_hex(digest: [u8; 32]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn parse_digest_hex(text: &str) -> [u8; 32] {
    assert_eq!(text.len(), 64, "BLAKE3 digest has 32 encoded bytes");
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .expect("cold digest hex byte");
    }
    digest
}

struct NoopWake;

impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

#[test]
fn snapshot_writer_peer() {
    let Some(path) = process_harness::peer_database() else {
        return;
    };
    let mut authority = futures_executor::block_on(TursoAuthority::open(&path))
        .expect("peer opens the live authority beside the owner");
    process_harness::report("OPENED");
    for line in process_harness::commands() {
        match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["WRITE", sequence] => {
                let sequence = sequence.parse::<u64>().expect("writer sequence");
                let receipt = futures_executor::block_on(
                    authority.record_source_observation(observation(sequence)),
                )
                .expect("concurrent process commits source observation");
                process_harness::report(&format!("WROTE {} {sequence}", receipt.sequence()));
            }
            ["PING"] => process_harness::report("PONG"),
            _ => process_harness::report("FAILED malformed writer command"),
        }
    }
}

#[test]
fn snapshot_cold_reopen_peer() {
    let Some(path) = process_harness::peer_database() else {
        return;
    };
    let (database, connection) = futures_executor::block_on(open_read_only(&path));
    let state = futures_executor::block_on(super::schema_preflight::classify(&connection))
        .expect("cold snapshot schema preflight");
    assert_eq!(state, super::schema_preflight::DatabaseState::Current);
    let mut integrity =
        futures_executor::block_on(connection.query("PRAGMA integrity_check(1)", ()))
            .expect("cold snapshot integrity query");
    let row = futures_executor::block_on(integrity.next())
        .expect("cold integrity result")
        .expect("integrity row");
    let verdict: String = row.get(0).expect("integrity verdict");
    assert_eq!(verdict, "ok");
    assert!(
        futures_executor::block_on(integrity.next())
            .expect("single integrity result")
            .is_none()
    );
    drop(integrity);
    process_harness::report("OPENED");
    let (sequence, count, observed_at) = futures_executor::block_on(read_latest_value(&connection));
    let (bytes, digest) = cold_file_digest(&path);
    process_harness::report(&format!(
        "SNAPSHOT {sequence} {count} {observed_at} {bytes} {}",
        digest_hex(digest)
    ));
    drop(connection);
    drop(database);
}

#[test]
fn same_engine_snapshot_is_consistent_during_live_multiprocess_ingest() {
    let source = Database::new("owner-snapshot-live-writer");
    let mut authority = futures_executor::block_on(TursoAuthority::open(&source.0))
        .expect("open live index authority");
    let baseline = futures_executor::block_on(seed_large_history(&mut authority));
    let destination = PrivateTestRoot::new();

    let snapshot_active = Arc::new(AtomicBool::new(false));
    let committed_during_snapshot = Arc::new(AtomicU64::new(0));
    let stop_writer = Arc::new(AtomicBool::new(false));
    let (ready_sender, ready_receiver) = mpsc::channel();
    let writer_path = source.0.clone();
    let writer_active = Arc::clone(&snapshot_active);
    let writer_commits = Arc::clone(&committed_during_snapshot);
    let writer_stop = Arc::clone(&stop_writer);
    let writer = thread::spawn(move || {
        let (mut peer, opened) = Peer::spawn(WRITER_PEER, &writer_path);
        ready_sender.send(opened).expect("send writer readiness");
        let mut sequence = baseline + 1;
        while !writer_stop.load(Ordering::Acquire) {
            let report = peer.ask(&format!("WRITE {sequence}"));
            assert_eq!(report, format!("WROTE {sequence} {sequence}"));
            if writer_active.load(Ordering::Acquire) {
                writer_commits.fetch_add(1, Ordering::AcqRel);
            }
            sequence += 1;
        }
        let ping = peer.ask("PING");
        let clean_exit = peer.release();
        (ping, clean_exit)
    });
    assert_eq!(
        ready_receiver
            .recv_timeout(Duration::from_secs(60))
            .expect("writer peer ready"),
        "OPENED"
    );

    let budget = AuthoritySnapshotBudget::new(64 * 1024 * 1024).expect("positive snapshot budget");
    let mut operation = futures_executor::block_on(authority.prepare_snapshot(
        destination.capability(),
        &destination.path,
        budget,
    ))
    .expect("prepare snapshot before beginning the active-write interval");
    snapshot_active.store(true, Ordering::Release);
    let snapshot_result = futures_executor::block_on(operation.run());
    snapshot_active.store(false, Ordering::Release);
    drop(operation);
    stop_writer.store(true, Ordering::Release);
    let (writer_ping, writer_exit) = writer.join().expect("writer peer thread");
    let snapshot =
        snapshot_result.unwrap_or_else(|failure| panic!("create live snapshot: {failure}"));
    assert_eq!(
        writer_ping, "PONG",
        "the source stayed responsive after snapshot"
    );
    assert!(writer_exit, "the independent writer process exited cleanly");
    assert!(
        committed_during_snapshot.load(Ordering::Acquire) > 0,
        "a real multiprocess source commit completed while VACUUM INTO was active"
    );

    snapshot
        .verify_named()
        .expect("snapshot receipt still names its output");
    assert!(snapshot.receipt().bytes() <= budget.max_output_bytes());
    assert!(snapshot.receipt().schema_version() > 0);
    assert_ne!(snapshot.receipt().digest(), [0; 32]);
    let source_head =
        futures_executor::block_on(authority.latest_source_observation(&snapshot_namespace()))
            .expect("read source head after snapshot")
            .expect("source head remains available");
    assert!(source_head.sequence() >= baseline);
    assert_eq!(
        source_head.observation().observed_at_ms(),
        source_head.sequence()
    );
    assert_eq!(
        source_head.observation().value(),
        &SourceObservationValue::KnownCount(source_head.sequence())
    );

    let (mut peer, opened) = Peer::spawn(COLD_REOPEN_PEER, snapshot.path());
    assert_eq!(
        opened, "OPENED",
        "another process reopened the exact snapshot read-only"
    );
    let cold = peer.next_report();
    let cold_exit = peer.release();
    assert!(cold_exit, "cold read-only process exited cleanly");
    let mut fields = cold.split_whitespace();
    assert_eq!(fields.next(), Some("SNAPSHOT"));
    let snapshot_sequence = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .expect("snapshot sequence");
    let snapshot_count = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .expect("snapshot value");
    let snapshot_observed_at = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .expect("snapshot observation time");
    let snapshot_bytes = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .expect("cold snapshot file byte count");
    let snapshot_digest = parse_digest_hex(fields.next().expect("cold snapshot BLAKE3 digest"));
    assert_eq!(fields.next(), None);
    assert_eq!(snapshot_sequence, snapshot_count);
    assert_eq!(snapshot_sequence, snapshot_observed_at);
    assert!(snapshot_sequence >= baseline);
    assert!(snapshot_sequence <= source_head.sequence());
    assert_eq!(snapshot_bytes, snapshot.receipt().bytes());
    assert_eq!(snapshot_digest, snapshot.receipt().digest());
    snapshot
        .verify_named()
        .expect("cold reopen did not replace the snapshot file");

    snapshot
        .staging_receipt()
        .remove_all(MAX_TEST_DIRECTORY_ENTRIES)
        .expect("remove completed test snapshot through exact receipt");
    destination.remove_all();
}

#[test]
fn cancelling_snapshot_run_retains_receipt_and_leaves_owner_usable() {
    let database = Database::new("owner-snapshot-cancel");
    let mut authority = futures_executor::block_on(TursoAuthority::open(&database.0))
        .expect("open cancellation-test authority");
    futures_executor::block_on(seed_large_history(&mut authority));
    let destination = PrivateTestRoot::new();
    let budget = AuthoritySnapshotBudget::new(64 * 1024 * 1024)
        .expect("positive cancellation-test budget");
    let mut operation = futures_executor::block_on(authority.prepare_snapshot(
        destination.capability(),
        &destination.path,
        budget,
    ))
    .expect("prepare before starting the cancellable execution");
    let stage_name = operation
        .staging_receipt()
        .expect("prepared operation owns exact stage receipt")
        .name()
        .to_owned();

    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);
    let mut run = Box::pin(operation.run());
    assert!(
        matches!(run.as_mut().poll(&mut context), Poll::Pending),
        "large VACUUM INTO yields so its run future can be cancelled"
    );
    drop(run);

    assert_eq!(
        operation
            .staging_receipt()
            .expect("cancelled operation still owns the exact receipt")
            .name(),
        stage_name
    );
    let retry = futures_executor::block_on(operation.run())
        .expect_err("a cancelled operation must not overwrite or restart its output");
    assert!(matches!(
        retry.error(),
        AuthoritySnapshotError::OperationCancelled
    ));
    assert_eq!(
        operation
            .staging_receipt()
            .expect("refused retry leaves the receipt in the operation")
            .name(),
        stage_name
    );

    let failure = operation.into_failure();
    let (cause, incomplete) = failure.into_parts();
    assert!(matches!(cause, AuthoritySnapshotError::OperationCancelled));
    let incomplete = incomplete.expect("cancelled operation returns exact incomplete artifact");
    assert_eq!(incomplete.staging_receipt().name(), stage_name);
    incomplete
        .verify_named()
        .expect("cancellation retained the exact staged directory");
    incomplete
        .staging_receipt()
        .remove_all(MAX_TEST_DIRECTORY_ENTRIES)
        .expect("remove cancelled output only through its exact receipt");
    destination.remove_all();

    let owner_head = futures_executor::block_on(
        authority.latest_source_observation(&snapshot_namespace()),
    )
    .expect("owner connection remains usable after cancelling engine work")
    .expect("large source history remains available");
    assert_eq!(owner_head.sequence(), SNAPSHOT_BASELINE);
}

#[test]
fn failed_attempt_keeps_receipt_and_refuses_a_substituted_name() {
    let oversized_path = PathBuf::from(format!("/{}", "x".repeat(40_000)));
    assert!(matches!(
        super::snapshot::snapshot_sql_path(&oversized_path),
        Err(AuthoritySnapshotError::InvalidDestinationPath)
    ));

    let database = Database::new("owner-snapshot-failure");
    let mut authority = futures_executor::block_on(TursoAuthority::open(&database.0))
        .expect("open failure-test authority");
    futures_executor::block_on(authority.record_source_observation(observation(1)))
        .expect("seed failure-test authority");
    let destination = PrivateTestRoot::new();

    let quoted_destination = PrivateTestRoot::with_suffix("'quoted");
    let budget = AuthoritySnapshotBudget::new(4 * 1024 * 1024)
        .expect("positive quoted-path test budget");
    let quoted = futures_executor::block_on(authority.prepare_snapshot(
        quoted_destination.capability(),
        &quoted_destination.path,
        budget,
    ))
    .expect_err("apostrophes are refused before the stage is created");
    assert!(matches!(
        quoted.error(),
        AuthoritySnapshotError::InvalidDestinationPath
    ));
    assert!(quoted.incomplete().is_none());
    assert!(quoted_destination
        .capability()
        .entries(1)
        .expect("inspect quoted root")
        .is_empty());
    quoted_destination.remove_all();

    let too_small = AuthoritySnapshotBudget::new(1).expect("positive small budget");
    let rejected = futures_executor::block_on(authority.prepare_snapshot(
        destination.capability(),
        &destination.path,
        too_small,
    ))
    .expect_err("a one-byte limit rejects the source before staging");
    assert!(matches!(
        rejected.error(),
        AuthoritySnapshotError::SourceExceedsBudget { .. }
    ));
    assert!(rejected.incomplete().is_none());
    assert!(
        destination
            .capability()
            .entries(1)
            .expect("inspect empty destination")
            .is_empty()
    );

    futures_executor::block_on(authority.raw_connection().execute("BEGIN", ()))
        .expect("hold an explicit owner transaction");
    let budget = AuthoritySnapshotBudget::new(4 * 1024 * 1024).expect("positive budget");
    let mut operation = futures_executor::block_on(authority.prepare_snapshot(
        destination.capability(),
        &destination.path,
        budget,
    ))
    .expect("preflight succeeds before the deliberately active transaction");
    let failure = futures_executor::block_on(operation.run())
        .expect_err("VACUUM INTO must reject an already active owner transaction");
    drop(operation);
    assert!(
        failure.incomplete().is_some(),
        "post-staging failure retains its receipt"
    );
    assert!(
        failure
            .incomplete()
            .expect("incomplete receipt")
            .verify_named()
            .is_ok()
    );
    futures_executor::block_on(authority.raw_connection().execute("ROLLBACK", ()))
        .expect("restore the owner connection after the refused operation");
    let (cause, incomplete) = failure.into_parts();
    assert!(matches!(cause, AuthoritySnapshotError::Owner(_)));
    let incomplete = incomplete.expect("failure retained exact incomplete artifact");

    #[cfg(unix)]
    {
        let stage = incomplete
            .path()
            .parent()
            .expect("staging parent")
            .to_owned();
        let moved = destination.path.join("moved-incomplete-stage");
        fs::rename(&stage, &moved).expect("move the pinned stage away");
        fs::create_dir(&stage).expect("put a replacement at its old name");
        let sentinel = stage.join("substituted.txt");
        fs::write(&sentinel, b"must survive receipt refusal").expect("write substitution sentinel");
        assert!(incomplete.verify_named().is_err());
        assert!(
            incomplete
                .staging_receipt()
                .remove_all(MAX_TEST_DIRECTORY_ENTRIES)
                .is_err()
        );
        assert_eq!(
            fs::read(&sentinel).expect("substitution remains untouched"),
            b"must survive receipt refusal"
        );

        fs::remove_dir_all(&stage).expect("remove test-only substitution");
        fs::rename(&moved, &stage).expect("restore the exact receipt name");
        incomplete
            .verify_named()
            .expect("restored receipt resolves to its original held identity");
        incomplete
            .staging_receipt()
            .remove_all(MAX_TEST_DIRECTORY_ENTRIES)
            .expect("remove retained failed artifact explicitly through its receipt");
    }
    #[cfg(not(unix))]
    {
        incomplete
            .staging_receipt()
            .remove_all(MAX_TEST_DIRECTORY_ENTRIES)
            .expect("remove retained failed artifact explicitly through its receipt");
    }
    destination.remove_all();

    let after =
        futures_executor::block_on(authority.latest_source_observation(&snapshot_namespace()))
            .expect("source remains readable after snapshot refusal");
    assert_eq!(after.expect("source head exists").sequence(), 1);
}
